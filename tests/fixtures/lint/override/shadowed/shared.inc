# Shaped after OE-core's u-boot-common.inc, required by several recipes: one of them
# (shared-kiss.bb) replaces these values, the other (shared-tools.bb) does not.
HOMEPAGE = "https://example.org/u-boot"
LICENSE = "GPL-2.0-or-later"
LIC_FILES_CHKSUM = "file://Licenses/README;md5=2ca5f2c35c8cc335f0a19756634782f1"
SRCREV = "127a42c7257a6ffbbd1575ed1cbaa8f5408a44b3"
SRC_URI = "git://example.org/u-boot.git;protocol=https;branch=master;tag=v${PV}"
# Replaced in this same file: shadowed wherever the file is used.
MY_SHARED = "first"
MY_SHARED = "second"
