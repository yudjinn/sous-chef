# do_install that only calls helpers: they may well use ${D}.
mytool_install_files() {
	install -d ${D}${bindir}
}

do_install() {
	mytool_install_files
	chmod 0755 ${B}/tool
}
