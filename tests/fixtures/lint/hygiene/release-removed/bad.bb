SUMMARY = "Constructs that later releases removed or renamed"
LICENSE = "GPLv2"
SRC_URI = "bzr://example.org/x;protocol=https"
S = "${WORKDIR}/x-${PV}"
DEPENDS = "virtual/${TARGET_PREFIX}gcc zlib"
SERIAL_CONSOLE = "115200 ttyS0"
inherit siteconfig cve-check
IMAGE_FEATURES += "debug-tweaks"
WARN_QA:append = " license-incompatible"
PNBLACKLIST[foo] = "broken"
FOO="bar"
do_install() {
    install -d ${D}${oldincludedir}
}
