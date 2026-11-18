# More legacy names for the fix: an override before the keyword, python and fakeroot
# functions, a package override with a suffix. The names at the end are not reported.
EXTRA_OECONF:qemux86:append = " --with-x"
SRC_URI:append:class-target = " file://t.patch"
python do_configure:prepend() {
    bb.note("configuring")
}
fakeroot do_install:append () {
	: # do_install_append stays in the body
}
RDEPENDS:${PN}-ptest:append:libc-glibc = " glibc-utils"
SRCREV_machine = "0123456789abcdef0123456789abcdef01234567"
FILES_SOLIBSDEV = ""
do_compile() {
	:
}
