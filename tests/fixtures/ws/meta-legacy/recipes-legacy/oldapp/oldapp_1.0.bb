SUMMARY = "Recipe written with pre-honister override syntax"
LICENSE = "MIT"
LIC_FILES_CHKSUM = "file://COPYING;md5=0835ade698e0bcf8506ecda2f7b4f302"

SRC_URI = "git://example.org/oldapp.git;protocol=https;branch=master \
           file://oldapp.patch \
           "
SRCREV = "89abcdef0123456789abcdef0123456789abcdef"
SRCREV_machine = "fedcba9876543210fedcba9876543210fedcba98"
S = "${WORKDIR}/git"

inherit autotoolsish oldclass

DEPENDS_append = " zlib"
EXTRA_OECONF += "${PACKAGECONFIG_CONFARGS}"
EXTRA_OECONF_append_qemux86 = " --enable-qemu"
SRC_URI_remove = "file://oldapp.patch"

RDEPENDS_${PN} = "bash"
FILES_${PN}-dev += "${includedir}/oldapp"
ALLOW_EMPTY_${PN}-doc = "1"

do_install() {
	install -d ${D}${bindir}
}

do_install_append() {
	install -d ${D}${datadir}/oldapp
}

pkg_postinst_${PN}() {
	echo "installed"
}
