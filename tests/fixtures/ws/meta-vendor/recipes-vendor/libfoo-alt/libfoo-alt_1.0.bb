SUMMARY = "Alternative implementation of the foo library"
HOMEPAGE = "https://example.org/libfoo-alt"
LICENSE = "MIT"
LIC_FILES_CHKSUM = "file://COPYING;md5=0835ade698e0bcf8506ecda2f7b4f302"

PROVIDES = "virtual/foo"

SRC_URI = "https://example.org/releases/libfoo-alt-${PV}.tar.xz"
SRC_URI[sha256sum] = "3333333333333333333333333333333333333333333333333333333333333333"

inherit autotoolsish

EXTRA_OECONF = "--enable-compat-foo"
