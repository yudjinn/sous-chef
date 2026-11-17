SUMMARY = "Out of order"
LICENSE = "MIT"
LIC_FILES_CHKSUM = "file://COPYING;md5=0835ade698e0bcf8506ecda2f7b4f302"

inherit autotools

SRC_URI = "https://example.org/x-${PV}.tar.gz"
DEPENDS = "zlib"
