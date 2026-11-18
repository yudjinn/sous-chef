# Legacy "_" override syntax (qemux86 / testdistro are a machine / distro of the fixture ws).
DEPENDS:append = " zlib"
SRC_URI:prepend = "file://first.patch "
EXTRA_OECONF:remove = "--enable-foo"
RDEPENDS:${PN} = "bash"
FILES:${PN}-dev += "${includedir}/x"
EXTRA_OECONF:append:qemux86 = " --enable-qemu"
KERNEL_DEVICETREE:qemux86 = "x.dtb"
EXTRA_OECONF:testdistro = "--with-distro"
EXTRA_OECONF:class-native = "--native"
do_install:append() {
	:
}
pkg_postinst:${PN}() {
	echo
}
export CFLAGS:append = " -g"
