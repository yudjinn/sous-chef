SUMMARY = "The foo library"
HOMEPAGE = "https://example.org/libfoo"
LICENSE = "BSD-3-Clause"
LIC_FILES_CHKSUM = "file://LICENSE;md5=3775480a712fc46a69647678acb234cb"

PROVIDES = "virtual/foo"

SRC_URI = "git://example.org/libfoo.git;protocol=https;branch=main"
SRCREV = "0123456789abcdef0123456789abcdef01234567"
S = "${WORKDIR}/git"

inherit autotoolsish

PACKAGECONFIG ??= "zlib"
PACKAGECONFIG[zlib] = "--with-zlib,--without-zlib,zlib,zlib"
PACKAGECONFIG[ssl] = "--with-ssl,--without-ssl,openssl"

RDEPENDS:${PN}-dev = "${PN}"
