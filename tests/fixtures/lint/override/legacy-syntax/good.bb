# Colon syntax, and names that contain "_" but are not overrides.
DEPENDS:append = " zlib"
SRC_URI:prepend = "file://first.patch "
RDEPENDS:${PN} = "bash"
KERNEL_DEVICETREE:qemux86 = "x.dtb"
SRCREV_machine = "0123456789abcdef0123456789abcdef01234567"
SRCREV_qemux86 = "0123456789abcdef0123456789abcdef01234567"
BBFILE_PRIORITY_core = "5"
LAYERSERIES_COMPAT_core = "scarthgap"
EXTRA_OECONF += "${PACKAGECONFIG_CONFARGS}"
PACKAGE_BEFORE_PN = ""
FILES_SOLIBSDEV = "${libdir}/lib*.so"
PREFERRED_VERSION_qemux86 = "1.0"
MY_SETTING_boardy = "boardy is not a machine"
do_install() {
	:
}
python do_report() {
    bb.plain("x")
}
