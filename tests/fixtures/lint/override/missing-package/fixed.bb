# Package-scoped variables without a package override.
RDEPENDS:${PN} = "bash"
FILES:${PN} += "${datadir}/x"
RRECOMMENDS:${PN}:append = " foo"
pkg_postinst:${PN}() {
	echo installed
}
ALLOW_EMPTY:${PN} = "1"
RPROVIDES:${PN}:append:qemux86 = " virtual-x"
