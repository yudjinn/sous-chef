do_install() {
	install -d ${D}${bindir}
	if [ -n "${EXTRA}" ]; then
		echo "{ braces } inside a body"
	fi
}

helper_function () {
	:
}
