# Workspace description for tests/differential.rs (`differential_scarthgap_mc`) and
# scripts/differential.sh: the Scarthgap release workspace (scripts/release-env.sh) with a
# second configuration, used by real-checks (docs/live-verification.md, "Three features
# never run for real"). Paths are relative to $SOUS_CHEF_LIVE. Format: wrynose.ws.
#
# The multiconfig is added to the workspace for the run only and removed afterwards:
#   build/conf/multiconfig/musl.conf:  TCLIBC = "musl"
#                                      TMPDIR = "${TOPDIR}/tmp-musl"
#   build/conf/local.conf, appended:   BBMULTICONFIG = "musl"
#                                      MCDEP_REALCHECKS = ""
#                                      MCDEP_REALCHECKS:pn-curl = "mc::musl:zlib:do_populate_sysroot"
#                                      do_install[mcdepends] = "${MCDEP_REALCHECKS}"
# (the recorded copies: tests/fixtures/outputs/live/real-checks/multiconfig/). Without
# conf/multiconfig/musl.conf the test returns at once. show-layers.out and show-appends.out
# were copied from runs/acc-releases/scarthgap/ (the same layers), so that only `bitbake -e`
# runs here.
name = scarthgap-mc
build = releases/scarthgap/build
init = oe:releases/scarthgap/openembedded-core
path =
dumps = runs/real-checks/scarthgap-mc
recorded = differential-scarthgap-mc
machine = qemux86-64
distro = poky
# sous-chef.toml [build_host]: the dumps' BUILD_ARCH / BUILD_OS (build-host)
build_arch = x86_64
build_os = linux
requires = conf/multiconfig/musl.conf
# what the second configuration changes
vars = BB_CURRENT_MC TCLIBC TCLIBCAPPEND TARGET_SYS MULTIMACH_TARGET_SYS PREFERRED_PROVIDER_virtual/libc STAGING_DIR_TARGET SSTATE_DIR DEPLOY_DIR
# the default configuration
recipes = zlib curl busybox quilt-native core-image-minimal virtual/libc
# the same in the musl configuration
recipes = mc:musl:zlib mc:musl:curl mc:musl:busybox mc:musl:quilt-native mc:musl:core-image-minimal mc:musl:virtual/libc
