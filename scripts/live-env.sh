#!/usr/bin/env bash
# Create (or re-check) the live Yocto environment sous-chef is tested against.
#
# See docs/live-environment.md and docs/milestones/M9-kas-live.md ("Limits on live runs").
# This script clones, runs `kas checkout` and `bitbake-setup init`, and (with --verify) runs
# parse-only BitBake commands. It never builds anything: no `kas build`, no image, no task.
#
# Safe to re-run: what already exists is kept; nothing is deleted or downloaded again.
#
# Usage: scripts/live-env.sh [--verify] [--no-bitbake-setup]
#   --verify            also run the parse-only commands (bitbake -p, bitbake -e curl,
#                       bitbake-layers show-*, bitbake-getvar) and report how long each took;
#                       raw output goes to $SOUS_CHEF_LIVE/runs/
#   --no-bitbake-setup  do not set up the bitbake-setup variant
#
# Environment:
#   SOUS_CHEF_LIVE   where the environment lives (default: ~/Development/sous-chef-live)

set -euo pipefail

# --- pinned versions -----------------------------------------------------------------
KAS_VERSION="5.5"
BITBAKE_SETUP_VERSION="2.19.0"
YOCTO_KISS_URL="https://github.com/bootlin/yocto-kiss"
YOCTO_KISS_BRANCH="wrynose"
YOCTO_KISS_COMMIT="cc621c5d5725a51af9abb7b0d531cf6f9264a468"
KAS_FILES="kas/kiss.yaml:kas/dogbonedark.yaml"
# yocto-kiss names branches, not commits, for the layers it downloads. These pins go into
# a kas lock file (kas/kiss.lock.yaml, next to the first kas file; kas reads it by itself)
# and into a bitbake-setup source-overrides file. Recorded 2026-11-17 from the branch heads.
PIN_BITBAKE="6e260eb6ab1631607ea414d9ff8ede1155faa5d9"            # bitbake, branch 2.18
PIN_OPENEMBEDDED_CORE="33a6bd6cc8a9a976902081642414bd67adc74879"  # branch wrynose
PIN_META_ARM="673e8d7c5dcd14de58092eb73e6ecc5eee5c1735"           # branch wrynose
# Only the bitbake-setup variant downloads these (yocto-kiss.conf.json lists them all).
PIN_META_YOCTO="b60511544c41f5d29d290681e15c9e302f747efe"         # branch wrynose
PIN_META_OPENEMBEDDED="c0fcd7cca3760ef24e087ac4a31b8f357cb204da"  # branch wrynose
PIN_META_XILINX="9ad7d683b1809de7c4307ac083635a9371d909f4"        # branch wrynose
BBSETUP_CONFIG="dogbonedark"
BBSETUP_SETUP_DIR="kiss-wrynose-dogbonedark"   # the name bitbake-setup derives for it

VERIFY=0
BITBAKE_SETUP=1
for arg in "$@"; do
    case "$arg" in
        --verify) VERIFY=1 ;;
        --no-bitbake-setup) BITBAKE_SETUP=0 ;;
        -h|--help) sed -n '2,17p' "$0"; exit 0 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

log() { printf '==> %s\n' "$*" >&2; }

# kas refuses to start when NETRC_FILE names a file that does not exist; every clone here
# is public, so drop it for this script only.
if [[ -n "${NETRC_FILE:-}" && ! -e "$NETRC_FILE" ]]; then
    log "NETRC_FILE=$NETRC_FILE does not exist; unsetting it for this script"
    unset NETRC_FILE
fi

# --- 1. the directory ----------------------------------------------------------------
LIVE="${SOUS_CHEF_LIVE:-$HOME/Development/sous-chef-live}"
mkdir -p "$LIVE"
LIVE="$(cd "$LIVE" && pwd)"
VENV="$LIVE/venv"
REPO="$LIVE/yocto-kiss"
BBBUILDS="$LIVE/bitbake-builds"
BBSETUP_DIR="$BBBUILDS/$BBSETUP_SETUP_DIR"
RUNS="$LIVE/runs"

# --- 2. kas (and bitbake-setup) in a private virtual environment ---------------------
if [[ ! -x "$VENV/bin/python" ]]; then
    log "creating virtual environment $VENV"
    python3 -m venv "$VENV"
fi
pip_version() { "$VENV/bin/python" -m pip show "$1" 2>/dev/null | sed -n 's/^Version: //p'; }
if [[ "$(pip_version kas)" != "$KAS_VERSION" ]]; then
    log "installing kas $KAS_VERSION into $VENV"
    "$VENV/bin/python" -m pip install --quiet --disable-pip-version-check "kas==$KAS_VERSION"
else
    log "kas $KAS_VERSION already installed"
fi
if [[ $BITBAKE_SETUP == 1 ]]; then
    if [[ "$(pip_version bitbake-setup)" != "$BITBAKE_SETUP_VERSION" ]]; then
        log "installing bitbake-setup $BITBAKE_SETUP_VERSION into $VENV"
        "$VENV/bin/python" -m pip install --quiet --disable-pip-version-check \
            "bitbake-setup==$BITBAKE_SETUP_VERSION"
    else
        log "bitbake-setup $BITBAKE_SETUP_VERSION already installed"
    fi
fi
KAS="$VENV/bin/kas"

# --- 3. yocto-kiss at the pinned commit ----------------------------------------------
if [[ ! -d "$REPO/.git" ]]; then
    log "cloning $YOCTO_KISS_URL ($YOCTO_KISS_BRANCH) into $REPO"
    git clone --quiet --branch "$YOCTO_KISS_BRANCH" "$YOCTO_KISS_URL" "$REPO"
fi
if [[ "$(git -C "$REPO" rev-parse HEAD)" != "$YOCTO_KISS_COMMIT" ]]; then
    if ! git -C "$REPO" cat-file -e "$YOCTO_KISS_COMMIT^{commit}" 2>/dev/null; then
        log "fetching $YOCTO_KISS_BRANCH to find $YOCTO_KISS_COMMIT"
        git -C "$REPO" fetch --quiet origin "$YOCTO_KISS_BRANCH"
    fi
    if [[ -n "$(git -C "$REPO" status --porcelain --untracked-files=no)" ]]; then
        log "error: $REPO has local changes and is not at $YOCTO_KISS_COMMIT; not touching it"
        exit 1
    fi
    log "checking out $YOCTO_KISS_COMMIT"
    git -C "$REPO" -c advice.detachedHead=false checkout --quiet "$YOCTO_KISS_COMMIT"
else
    log "yocto-kiss already at $YOCTO_KISS_COMMIT"
fi

# --- 4. kas checkout (never kas build) -----------------------------------------------
# Writes a file only when its content differs, so a re-run leaves timestamps alone.
write_if_changed() {  # <path> <content>
    if [[ ! -f "$1" || "$(cat "$1")" != "$2" ]]; then
        log "writing $1"
        printf '%s\n' "$2" > "$1"
    fi
}
cd "$REPO"
write_if_changed "$REPO/kas/kiss.lock.yaml" "# Written by sous-chef scripts/live-env.sh: pins the layers yocto-kiss names by branch.
header:
  version: 14
overrides:
  repos:
    bitbake:
      commit: $PIN_BITBAKE
    meta-arm:
      commit: $PIN_META_ARM
    openembedded-core:
      commit: $PIN_OPENEMBEDDED_CORE"
# kas checkout leaves clones that already have the pinned commit alone (no network) and
# rewrites build/conf/bblayers.conf and local.conf with the same content.
# When every clone is already at its pin and build/conf exists, skip kas checkout so a
# re-run touches nothing (kas would rewrite build/conf/*.conf and the clones' git metadata
# with the same content).
at_pin() { [[ "$(git -C "$REPO/$1" rev-parse HEAD 2>/dev/null)" == "$2" ]]; }
if [[ -f "$REPO/build/conf/bblayers.conf" && -f "$REPO/build/conf/local.conf" ]] && \
        at_pin bitbake "$PIN_BITBAKE" && at_pin openembedded-core "$PIN_OPENEMBEDDED_CORE" && \
        at_pin meta-arm "$PIN_META_ARM"; then
    log "kas checkout already done (layers at their pins, build/conf present)"
else
    log "kas checkout $KAS_FILES"
    mkdir -p "$RUNS"
    rc=0
    "$KAS" checkout "$KAS_FILES" > "$RUNS/kas-checkout.out" 2>&1 || rc=$?
    grep -v ' - INFO ' "$RUNS/kas-checkout.out" >&2 || true
    if [[ $rc != 0 ]]; then
        log "error: kas checkout failed (exit $rc, log: $RUNS/kas-checkout.out)"
        exit 1
    fi
fi

# --- 5. host tools BitBake requires (reported, never installed) ----------------------
MISSING_HOSTTOOLS=""
hosttools() {
    # The fatal HOSTTOOLS list in OE-core's bitbake.conf ("Tools needed to run builds").
    sed -n '/^HOSTTOOLS += " \\$/,/^"$/p' "$REPO/openembedded-core/meta/conf/bitbake.conf" \
        | tr -d '\\"' | sed 's/HOSTTOOLS += //' | tr -s ' \n' '\n' | sed '/^$/d'
}
for t in $(hosttools); do
    command -v "$t" >/dev/null 2>&1 || MISSING_HOSTTOOLS="$MISSING_HOSTTOOLS $t"
done
MISSING_HOSTTOOLS="${MISSING_HOSTTOOLS# }"
if [[ -n "$MISSING_HOSTTOOLS" ]]; then
    log "WARNING: host tools BitBake requires are missing: $MISSING_HOSTTOOLS"
    log "         every BitBake command (bitbake -p, -e, bitbake-layers, bitbake-getvar) fails"
    log "         until they are installed. This script does not install anything."
fi

# --- 6. the bitbake-setup variant (reuses the kas clones) ----------------------------
if [[ $BITBAKE_SETUP == 1 ]]; then
    OVERRIDES="$LIVE/bitbake-setup-sources.json"
    write_if_changed "$OVERRIDES" "{
  \"sources\": {
    \"bitbake\": {\"local\": {\"path\": \"$REPO/bitbake\"}},
    \"openembedded-core\": {\"local\": {\"path\": \"$REPO/openembedded-core\"}},
    \"meta-arm\": {\"local\": {\"path\": \"$REPO/meta-arm\"}},
    \"meta-kiss\": {\"local\": {\"path\": \"$REPO\"}},
    \"meta-yocto\": {\"git-remote\": {\"uri\": \"https://git.yoctoproject.org/meta-yocto\", \"branch\": \"wrynose\", \"rev\": \"$PIN_META_YOCTO\"}},
    \"meta-openembedded\": {\"git-remote\": {\"uri\": \"https://git.openembedded.org/meta-openembedded.git\", \"branch\": \"wrynose\", \"rev\": \"$PIN_META_OPENEMBEDDED\"}},
    \"meta-xilinx\": {\"git-remote\": {\"uri\": \"https://github.com/Xilinx/meta-xilinx.git\", \"branch\": \"wrynose\", \"rev\": \"$PIN_META_XILINX\"}}
  }
}"
    if [[ ! -d "$BBSETUP_DIR/layers" ]]; then
        log "bitbake-setup init $BBSETUP_CONFIG into $BBBUILDS"
        mkdir -p "$RUNS"
        if ! (cd "$LIVE" && "$VENV/bin/bitbake-setup" --color never \
                --setting default top-dir-prefix "$LIVE" \
                --setting default top-dir-name bitbake-builds \
                init --non-interactive --source-overrides "$OVERRIDES" \
                "$REPO/yocto-kiss.conf.json" "$BBSETUP_CONFIG") > "$RUNS/bitbake-setup-init.out" 2>&1; then
            log "bitbake-setup init failed (log: $RUNS/bitbake-setup-init.out):"
            grep -v '^$' "$RUNS/bitbake-setup-init.out" | tail -2 >&2 || true
        fi
    else
        log "bitbake-setup build already initialised in $BBSETUP_DIR"
    fi
    # bitbake-setup enables the distro and machine fragments last, with bitbake-config-build,
    # which needs BitBake to start (and so the host tools). Finish that step when it can.
    if [[ -f "$BBSETUP_DIR/build/init-build-env" ]] && \
            ! grep -qs 'machine/dogbonedark' "$BBSETUP_DIR/build/conf/toolcfg.conf"; then
        if [[ -z "$MISSING_HOSTTOOLS" ]]; then
            log "enabling fragments distro/kiss machine/dogbonedark in the bitbake-setup build"
            bash -c ". '$BBSETUP_DIR/build/init-build-env' >/dev/null && \
                bitbake-config-build enable-fragment distro/kiss machine/dogbonedark"
        else
            log "bitbake-setup build: fragments distro/kiss machine/dogbonedark not enabled yet"
            log "         (needs the missing host tools; re-run this script once they are installed)"
        fi
    fi
fi

# --- 7. report -----------------------------------------------------------------------
commit_of() { git -C "$1" rev-parse HEAD 2>/dev/null || echo '?'; }
bitbake_version() {
    sed -n 's/^__version__ = "\(.*\)"/\1/p' "$REPO/bitbake/lib/bb/__init__.py" 2>/dev/null
}
cat <<EOF

sous-chef live environment
  SOUS_CHEF_LIVE     $LIVE
  venv               $VENV
  kas                $KAS ($("$KAS" --version 2>&1 | head -1))
  repository         $REPO
  kas files          $REPO/kas/kiss.yaml + $REPO/kas/dogbonedark.yaml
  kas lock file      $REPO/kas/kiss.lock.yaml
  build directory    $REPO/build
  bitbake            $(bitbake_version)
  yocto-kiss         $(commit_of "$REPO") ($YOCTO_KISS_BRANCH)
  bitbake            $(commit_of "$REPO/bitbake")
  openembedded-core  $(commit_of "$REPO/openembedded-core")
  meta-arm           $(commit_of "$REPO/meta-arm")
EOF
if [[ $BITBAKE_SETUP == 1 ]]; then
    cat <<EOF
  bitbake-setup      $VENV/bin/bitbake-setup ($BITBAKE_SETUP_VERSION)
  bitbake-setup dir  $BBSETUP_DIR
  init-build-env     $BBSETUP_DIR/build/init-build-env
  meta-yocto         $(commit_of "$BBSETUP_DIR/layers/meta-yocto")
  meta-openembedded  $(commit_of "$BBSETUP_DIR/layers/meta-openembedded")
  meta-xilinx        $(commit_of "$BBSETUP_DIR/layers/meta-xilinx")
EOF
fi
cat <<EOF
  missing host tools ${MISSING_HOSTTOOLS:-none}
  disk               $(du -sh "$LIVE" | cut -f1)

Run a parse-only command:
  cd $REPO && env -u NETRC_FILE $KAS shell $KAS_FILES -c 'bitbake -e curl'
EOF

# --- 8. --verify: parse-only commands, timed -----------------------------------------
if [[ $VERIFY == 1 ]]; then
    mkdir -p "$RUNS"
    echo
    echo "parse-only commands (output in $RUNS):"
    run_timed() {  # <name> <command...>
        local name="$1"; shift
        local start end rc=0
        start=$(date +%s.%N)
        "$@" > "$RUNS/$name.out" 2>&1 || rc=$?
        end=$(date +%s.%N)
        printf '  %-28s exit %-3s %7.1fs\n' "$name" "$rc" "$(echo "$end - $start" | bc)"
    }
    kas_c() { "$KAS" shell "$KAS_FILES" -c "$1"; }
    run_timed kas-dump              "$KAS" dump "$KAS_FILES"
    run_timed bitbake-p             kas_c 'bitbake -p'
    run_timed bitbake-e-curl        kas_c 'bitbake -e curl'
    run_timed show-layers           kas_c 'bitbake-layers show-layers'
    run_timed show-appends          kas_c 'bitbake-layers show-appends'
    run_timed show-overlayed        kas_c 'bitbake-layers show-overlayed'
    run_timed getvar-T-value        kas_c 'bitbake-getvar -r curl --value T'
    run_timed getvar-T              kas_c 'bitbake-getvar -r curl T'
    if [[ $BITBAKE_SETUP == 1 && -f "$BBSETUP_DIR/build/init-build-env" ]]; then
        run_timed bitbake-setup-bitbake-p \
            bash -c ". '$BBSETUP_DIR/build/init-build-env' >/dev/null && bitbake -p"
    fi
fi
