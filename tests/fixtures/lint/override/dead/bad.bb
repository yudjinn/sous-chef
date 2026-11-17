SUMMARY = "Overrides that match nothing in the fixture workspace"
LICENSE = "MIT"
EXTRA_OECONF:append:boradx = " --typo"
SRC_URI:append:qemuarm = " file://arm.patch"
do_configure:nosuchdistro() {
	:
}
RDEPENDS:${PN}:nosuch = "bash"
