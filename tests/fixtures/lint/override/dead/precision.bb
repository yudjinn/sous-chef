SUMMARY = "lint-precision: a misspelt operation, and a misspelt machine"
LICENSE = "MIT"
# Trimmed from OE-core (wrynose) recipes-devtools/python/python3-wcwidth_0.6.0.bb:11-14.
do_install_ptest:aapend() {
      install -d ${D}${PTEST_PATH}/bin
}
EXTRA_OECONF:append:boradx = " --typo"
