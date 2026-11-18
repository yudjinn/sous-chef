SUMMARY = "The same, written for current releases"
LICENSE = "GPL-2.0-only"
SRC_URI = "git://example.org/x;protocol=https;branch=main"
S = "${UNPACKDIR}/x-${PV}"
DEPENDS = "virtual/cross-cc zlib"
SERIAL_CONSOLES = "115200;ttyS0"
inherit create-spdx
IMAGE_FEATURES += "allow-empty-password allow-root-login empty-root-password"
WARN_QA:append = " license-exception"
SKIP_RECIPE[foo] = "broken"
FOO = "bar"
IMAGE_FEATURES:remove = "debug-tweaks"
do_install() {
    install -d ${D}${includedir}
}
