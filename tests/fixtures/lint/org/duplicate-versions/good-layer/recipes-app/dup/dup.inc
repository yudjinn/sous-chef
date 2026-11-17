SUMMARY = "Duplicated tool"
HOMEPAGE = "https://example.org/dup"
LICENSE = "MIT"

SRC_URI = "https://example.org/dup-${PV}.tar.gz"

DEPENDS = "zlib"
EXTRA_OECONF = "--disable-docs"

do_install() {
    install -d ${D}${bindir}
    install -m 0755 dup ${D}${bindir}
}
