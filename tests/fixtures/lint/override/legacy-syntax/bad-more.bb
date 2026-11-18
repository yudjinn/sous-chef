# More legacy names for the fix: an override before the keyword, python and fakeroot
# functions, a package override with a suffix. The names at the end are not reported.
EXTRA_OECONF_qemux86_append = " --with-x"
SRC_URI_append_class-target = " file://t.patch"
python do_configure_prepend() {
    bb.note("configuring")
}
fakeroot do_install_append () {
	: # do_install_append stays in the body
}
RDEPENDS_${PN}-ptest_append_libc-glibc = " glibc-utils"
SRCREV_machine = "0123456789abcdef0123456789abcdef01234567"
FILES_SOLIBSDEV = ""
do_compile() {
	:
}
