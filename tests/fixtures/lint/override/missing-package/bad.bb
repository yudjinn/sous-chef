# Package-scoped variables without a package override.
RDEPENDS = "bash"
FILES += "${datadir}/x"
RRECOMMENDS:append = " foo"
pkg_postinst() {
	echo installed
}
ALLOW_EMPTY = "1"
RPROVIDES:append:qemux86 = " virtual-x"
