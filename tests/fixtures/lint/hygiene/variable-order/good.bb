SUMMARY = "In order"
DESCRIPTION = "Follows the OE style guide."
HOMEPAGE = "https://example.org/x"
LICENSE = "MIT"
LIC_FILES_CHKSUM = "file://COPYING;md5=0835ade698e0bcf8506ecda2f7b4f302"

DEPENDS = "zlib"

SRC_URI = "https://example.org/x-${PV}.tar.gz"
SRC_URI[sha256sum] = "1111111111111111111111111111111111111111111111111111111111111111"

S = "${WORKDIR}/x-${PV}"

inherit autotools

PACKAGECONFIG ??= "zlib"
PACKAGECONFIG[zlib] = "--with-zlib,--without-zlib,zlib"
EXTRA_OECONF = "--disable-nls"
MY_LOCAL_SETTING = "anything goes"

do_install:append() {
	install -d ${D}${sysconfdir}
}

FILES:${PN} += "${sysconfdir}"
RDEPENDS:${PN} = "bash"
BBCLASSEXTEND = "native"
