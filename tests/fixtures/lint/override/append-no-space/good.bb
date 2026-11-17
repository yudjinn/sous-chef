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
# OE-core no-static-libs.inc:1,24: the referenced value starts with the space.
DISABLE_STATIC ?= " --disable-static"
DISABLE_STATIC:pn-musl = ""
EXTRA_OECONF:append = "${DISABLE_STATIC}"
# OE-core u-boot_2026.01.bb:16-27: the value starts with a continuation, so with spaces.
SRC_URI_RISCV = "\
    file://u-boot-riscv-isa_clear.cfg \
    "
SRC_URI:append:riscv32 = "${SRC_URI_RISCV}"
# A variable this file does not set cannot be judged; nor can python at the end.
DEPENDS:append = "${DEPENDS_FROM_ELSEWHERE}"
DEPENDS:prepend = "${PREPEND_FROM_ELSEWHERE}"
RDEPENDS:${PN}:prepend = "bash ${@'x ' if d.getVar('X') else ''}"
