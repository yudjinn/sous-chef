SUMMARY = "Duplicated tool"
HOMEPAGE = "https://example.org/dup"
LICENSE = "MIT"
LIC_FILES_CHKSUM = "file://COPYING;md5=22222222222222222222222222222222"

SRC_URI = "https://example.org/dup-${PV}.tar.gz"
SRC_URI[sha256sum] = "2222222222222222222222222222222222222222222222222222222222222222"

DEPENDS = "zlib openssl"
EXTRA_OECONF = "--disable-docs"

do_install() {
    install -d ${D}${bindir}
    install -m 0755 dup ${D}${bindir}
}
