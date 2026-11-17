SUMMARY = "Overrides the fixture workspace knows"
LICENSE = "MIT"
EXTRA_OECONF:append:boardx = " --board"
EXTRA_OECONF:append:qemux86 = " --qemu"
SRC_URI:append:socx = " file://soc.patch"
DEPENDS:append:class-native = " zlib-native"
CFLAGS:append:testdistro = " -g"
CFLAGS:append:arm = " -marm"
EXTRA_OECONF:append:pn-other = " --other"
EXTRA_OECONF:append:autotoolsish = " --class"
RDEPENDS:${PN}-extra = "bash"
FILES:somepackage-doc = "${docdir}"
INSANE_SKIP:somepackage = "ldflags"
do_configure:qemux86() {
	:
}
