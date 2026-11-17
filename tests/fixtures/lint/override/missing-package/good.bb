# Every package-scoped variable names its package.
RDEPENDS:${PN} = "bash"
FILES:${PN} += "${datadir}/x"
RRECOMMENDS:${PN}:append = " foo"
pkg_postinst:${PN}() {
	echo installed
}
ALLOW_EMPTY:${PN}-dev = "1"
RPROVIDES:libfoo = "virtual-x"
SUMMARY = "SUMMARY also has a recipe-wide meaning"
LICENSE = "MIT"
RDEPENDS[doc] = "a flag, not a value"
RDEPENDS_${PN} = "legacy syntax is another rule's business"
