# Stand-in for autotools.bbclass.
#
# Interface (set these in the recipe):
#   EXTRA_OECONF           extra arguments to configure
#   AUTOTOOLS_SCRIPT_PATH  directory holding the configure script

EXTRA_OECONF ?= ""
AUTOTOOLS_SCRIPT_PATH ?= "${S}"
EXTRA_AUTORECONF ?= "--exclude=autopoint"

DEPENDS:prepend = "autoconf-native automake-native "

autotoolsish_do_autoreconf() {
	autoreconf -Wcross --verbose --install --force ${EXTRA_AUTORECONF}
}
addtask autoreconf after do_patch before do_configure

autotoolsish_do_configure() {
	${AUTOTOOLS_SCRIPT_PATH}/configure --prefix=${prefix} ${EXTRA_OECONF}
}

autotoolsish_do_compile() {
	oe_runmake
}

autotoolsish_do_install() {
	oe_runmake 'DESTDIR=${D}' install
}

EXPORT_FUNCTIONS do_autoreconf do_configure do_compile do_install
