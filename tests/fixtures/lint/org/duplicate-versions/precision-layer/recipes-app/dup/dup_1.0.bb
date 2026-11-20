SUMMARY = "Duplicated tool"
HOMEPAGE = "https://example.org/dup"
LICENSE = "MIT"
LIC_FILES_CHKSUM = "file://COPYING;md5=11111111111111111111111111111111"

SRC_URI = "https://example.org/dup-${PV}.tar.gz"
SRC_URI[sha256sum] = "1111111111111111111111111111111111111111111111111111111111111111"

DEPENDS = "zlib"
EXTRA_OECONF = "--disable-docs"

do_install() {
    install -d ${D}${bindir}
    install -m 0755 dup ${D}${bindir}
}
