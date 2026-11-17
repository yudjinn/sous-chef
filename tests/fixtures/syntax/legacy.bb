# Legacy "_" override syntax (true positives).
DEPENDS_append = " zlib"
SRC_URI_prepend = "file://first.patch "
EXTRA_OECONF_remove = "--enable-foo"
RDEPENDS_${PN} = "bash"
FILES_${PN}-dev = "${includedir}"
EXTRA_OECONF_append_qemux86 = " --enable-qemu"
do_install_append() {
	:
}

# Names that contain "_" but are not legacy overrides (false positives to avoid).
SRCREV_machine = "0123456789abcdef0123456789abcdef01234567"
BBFILE_PRIORITY_core = "5"
LAYERSERIES_COMPAT_core = "scarthgap"
EXTRA_OECONF += "${PACKAGECONFIG_CONFARGS}"
PACKAGE_BEFORE_PN = ""
do_install() {
	:
}
