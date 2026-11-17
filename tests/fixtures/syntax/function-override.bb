do_install:append() {
	echo "append"
}

do_install:prepend:class-target() {
	echo "prepend for target"
}

do_configure:qemux86 () {
	echo "machine-specific configure"
}
