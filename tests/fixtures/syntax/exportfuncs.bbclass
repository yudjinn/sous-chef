exportfuncs_do_compile() {
	oe_runmake
}

exportfuncs_do_install() {
	oe_runmake install
}

EXPORT_FUNCTIONS do_compile do_install
