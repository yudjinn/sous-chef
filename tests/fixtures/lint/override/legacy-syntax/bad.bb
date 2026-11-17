# Legacy "_" override syntax (qemux86 / testdistro are a machine / distro of the fixture ws).
DEPENDS_append = " zlib"
SRC_URI_prepend = "file://first.patch "
EXTRA_OECONF_remove = "--enable-foo"
RDEPENDS_${PN} = "bash"
FILES_${PN}-dev += "${includedir}/x"
EXTRA_OECONF_append_qemux86 = " --enable-qemu"
KERNEL_DEVICETREE_qemux86 = "x.dtb"
EXTRA_OECONF_testdistro = "--with-distro"
EXTRA_OECONF_class-native = "--native"
do_install_append() {
	:
}
pkg_postinst_${PN}() {
	echo
}
export CFLAGS_append = " -g"
