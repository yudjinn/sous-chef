# Spaces where lists need them; colon lists, scalars, python and functions left alone.
DEPENDS:append = " zlib"
SRC_URI:prepend = "file://first.patch "
EXTRA_OECONF:append = " \
    --enable-foo \
    "
FILESEXTRAPATHS:prepend := "${THISDIR}/files:"
PATH:prepend = "/opt/bin:"
SUMMARY:append = "(patched)"
SRC_URI:append = "${@' file://x.patch' if d.getVar('X') else ''}"
MY_UNKNOWN_THING:append = "x"
DEPENDS:append += "openssl"
SRC_URI[sha256sum] = "1111111111111111111111111111111111111111111111111111111111111111"
do_install:append() {
	echo done
}
